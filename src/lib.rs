use std::sync::Arc;

use axum::{Router, http::StatusCode, routing::get};
use tokio::net::TcpListener;

mod allowlist;
mod bangs;
mod cache;
mod compat;
pub mod config;
mod dict;
mod error;
pub mod extension_proxy;
pub mod http;
pub mod ubo;

pub use config::Config;
pub use extension_proxy::{ExtensionProxyConfig, ExtensionProxyService};
pub use http::app as ubo_app;
pub use ubo::UboService;

pub async fn run() -> Result<(), String> {
    config::load_dotenv();

    let ubo_config = Arc::new(Config::from_env()?);
    let ubo_service = UboService::new(ubo_config);
    ubo_service.spawn_cache_cleanup();
    ubo_service.spawn_stats_logger();

    let extension_proxy_config = Arc::new(ExtensionProxyConfig::from_env()?);
    let extension_proxy_service = ExtensionProxyService::new(extension_proxy_config);
    let dictionary_service = dict::DictionaryService::from_env()?;
    dictionary_service.spawn_refresh();

    let bind_addr = config::bind_addr();
    let listener = TcpListener::bind(&bind_addr)
        .await
        .map_err(|err| format!("failed to bind {bind_addr}: {err}"))?;

    ubo_service.preload_assets();

    axum::serve(
        listener,
        app_with_dictionary(ubo_service, extension_proxy_service, dictionary_service),
    )
    .await
    .map_err(|err| format!("server error: {err}"))
}

pub fn app(ubo_service: UboService, extension_proxy_service: ExtensionProxyService) -> Router {
    app_with_dictionary(
        ubo_service,
        extension_proxy_service,
        dict::DictionaryService::default(),
    )
}

fn app_with_dictionary(
    ubo_service: UboService,
    extension_proxy_service: ExtensionProxyService,
    dictionary_service: dict::DictionaryService,
) -> Router {
    Router::new()
        .merge(compat::app().expect("compatibility routes must be valid"))
        .merge(dict::app(dictionary_service))
        .route("/healthz", get(no_content))
        .route("/connectivitycheck", get(no_content))
        .route("/bangs.json", get(bangs::get).head(bangs::head))
        .nest("/ubo", ubo_app(ubo_service))
        .nest("/ext", extension_proxy::app(extension_proxy_service))
}

async fn no_content() -> StatusCode {
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use std::{
        net::SocketAddr,
        sync::{Arc, Mutex},
    };

    use axum::{
        Router,
        body::Body,
        http::{
            Method, Request, StatusCode, Uri,
            header::{
                ACCEPT_ENCODING, ACCESS_CONTROL_ALLOW_ORIGIN, CACHE_CONTROL, CONTENT_ENCODING,
                CONTENT_LENGTH, CONTENT_TYPE, ETAG, IF_NONE_MATCH, LOCATION, VARY,
            },
        },
        response::Response,
    };
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tower::ServiceExt;
    use url::Url;

    use crate::{
        Config, ExtensionProxyConfig, ExtensionProxyService, UboService, app,
        ubo::{
            assets::to_pretty_json_4,
            tags::sha256_hex,
            urls::{posix_dirname, posix_join},
        },
        ubo_app,
    };

    #[tokio::test]
    async fn assets_route_rewrites_manifest_and_headers() {
        let filter_body = "!#include sub/include.txt\n! Diff-Path: patches/a.patch#frag\n";
        let source = fixture_server(filter_body).await;
        let source_url = format!("{source}/filters/easylist.txt");
        let manifest = json!({
            "assets.json": {
                "content": "internal",
                "contentURL": "assets/assets.json"
            },
            "easylist": {
                "content": "filters",
                "title": "EasyList",
                "contentURL": source_url,
                "cdnURLs": ["assets/local.txt"],
                "patchURLs": ["patch"]
            }
        });
        let manifest_string = to_pretty_json_4(&manifest).unwrap();
        let manifest_checksum = sha256_hex(manifest_string.as_bytes());
        let assets_source = fixture_server(&manifest_string).await;

        let service = test_service(
            "http://proxy.local/",
            &format!("{assets_source}/assets.json"),
            &manifest_checksum,
        );
        let response = ubo_app(service)
            .oneshot(
                Request::builder()
                    .uri("/assets.json")
                    .header(ACCEPT_ENCODING, "br")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_ENCODING], "br");
        assert_eq!(response.headers()[CACHE_CONTROL], "public, max-age=3600");
        assert_eq!(response.headers()[VARY], "Accept-Encoding");
        assert!(response.headers().contains_key(ETAG));

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let decoded = brotli_decompress(&body);
        let rewritten: Value = serde_json::from_str(&decoded).unwrap();
        let easylist = &rewritten["easylist"];
        assert!(easylist.get("cdnURLs").is_none());
        assert_eq!(easylist["contentURL"][1], "assets/local.txt");
        assert!(
            easylist["contentURL"][0]
                .as_str()
                .unwrap()
                .starts_with("http://proxy.local/easylist/")
        );
        assert!(
            easylist["patchURLs"][0]
                .as_str()
                .unwrap()
                .starts_with("http://proxy.local/easylist/")
        );
    }

    #[tokio::test]
    async fn filterlist_route_expands_relative_allowlist_and_304s() {
        let filter_body = "!#include sub/include.txt\n! Diff-Path: patches/a.patch#frag\n";
        let source = fixture_server(filter_body).await;
        let source_url = format!("{source}/filters/easylist.txt");
        let manifest = json!({
            "assets.json": {
                "content": "internal",
                "contentURL": "assets/assets.json"
            },
            "easylist": {
                "content": "filters",
                "contentURL": source_url
            }
        });
        let manifest_string = to_pretty_json_4(&manifest).unwrap();
        let manifest_checksum = sha256_hex(manifest_string.as_bytes());
        let assets_source = fixture_server(&manifest_string).await;
        let service = test_service(
            "http://proxy.local/",
            &format!("{assets_source}/assets.json"),
            &manifest_checksum,
        );

        let assets = service.handle_assets().await.unwrap();
        let decoded_assets = brotli_decompress(&assets.body);
        let manifest: Value = serde_json::from_str(&decoded_assets).unwrap();
        let filter_url = manifest["easylist"]["contentURL"].as_str().unwrap();
        let filter_path = Url::parse(filter_url).unwrap().path().to_string();

        let response = ubo_app(service.clone())
            .oneshot(
                Request::builder()
                    .uri(&filter_path)
                    .header(ACCEPT_ENCODING, "br")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let etag = response.headers()[ETAG].to_str().unwrap().to_string();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(brotli_decompress(&body), filter_body);

        let not_modified = ubo_app(service.clone())
            .oneshot(
                Request::builder()
                    .uri(&filter_path)
                    .header(ACCEPT_ENCODING, "br")
                    .header(IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);

        let include_path = posix_join(
            &posix_dirname(filter_path.trim_start_matches('/')),
            "sub/include.txt",
        );
        assert!(
            service
                .allowlist
                .get_urls_for_path(&include_path)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn missing_brotli_header_is_406() {
        let service = test_service(
            "http://proxy.local/ubo/",
            "http://127.0.0.1:9/assets.json",
            "unused",
        );
        let response = ubo_app(service)
            .oneshot(
                Request::builder()
                    .uri("/assets.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
    }

    #[tokio::test]
    async fn combined_app_serves_ubo_under_prefix_only() {
        let manifest = json!({
            "assets.json": {
                "content": "internal",
                "contentURL": "assets/assets.json"
            }
        });
        let manifest_string = to_pretty_json_4(&manifest).unwrap();
        let manifest_checksum = sha256_hex(manifest_string.as_bytes());
        let assets_source = fixture_server(&manifest_string).await;
        let ubo_service = test_service(
            "http://proxy.local/ubo/",
            &format!("{assets_source}/assets.json"),
            &manifest_checksum,
        );
        let app = app(ubo_service, test_extension_proxy_service());

        let prefixed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ubo/assets.json")
                    .header(ACCEPT_ENCODING, "br")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(prefixed.status(), StatusCode::OK);

        let unprefixed = app
            .oneshot(
                Request::builder()
                    .uri("/assets.json")
                    .header(ACCEPT_ENCODING, "br")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unprefixed.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn combined_app_serves_extension_proxy_under_prefix_only() {
        let ubo_service = test_service(
            "http://proxy.local/ubo/",
            "http://127.0.0.1:9/assets.json",
            "unused",
        );
        let app = app(ubo_service, test_extension_proxy_service());

        let prefixed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ext/proxy")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(prefixed.status(), StatusCode::BAD_REQUEST);

        let unprefixed = app
            .oneshot(
                Request::builder()
                    .uri("/proxy")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unprefixed.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn combined_app_has_lightweight_healthcheck() {
        let ubo_service = test_service(
            "http://proxy.local/ubo/",
            "http://127.0.0.1:9/assets.json",
            "unused",
        );
        let response = app(ubo_service, test_extension_proxy_service())
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn combined_app_serves_bangs_json_with_cache_headers() {
        let ubo_service = test_service(
            "http://proxy.local/ubo/",
            "http://127.0.0.1:9/assets.json",
            "unused",
        );
        let app = app(ubo_service, test_extension_proxy_service());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/bangs.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[CACHE_CONTROL],
            "public, max-age=86400, stale-if-error=604800"
        );
        assert_eq!(response.headers()[ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        assert_eq!(
            response.headers()[CONTENT_TYPE],
            "application/json; charset=utf-8"
        );
        assert!(response.headers().contains_key(ETAG));
        let content_length = response.headers()[CONTENT_LENGTH]
            .to_str()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let etag = response.headers()[ETAG].to_str().unwrap().to_string();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.len(), content_length);
        assert!(body.starts_with(b"// Generated at "));

        let not_modified = app
            .oneshot(
                Request::builder()
                    .uri("/bangs.json")
                    .header(IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
        let body = not_modified.into_body().collect().await.unwrap().to_bytes();
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn combined_app_serves_bangs_head_without_body() {
        let ubo_service = test_service(
            "http://proxy.local/ubo/",
            "http://127.0.0.1:9/assets.json",
            "unused",
        );
        let response = app(ubo_service, test_extension_proxy_service())
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/bangs.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key(CONTENT_LENGTH));
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn combined_app_rejects_unsupported_bangs_methods() {
        let ubo_service = test_service(
            "http://proxy.local/ubo/",
            "http://127.0.0.1:9/assets.json",
            "unused",
        );
        let response = app(ubo_service, test_extension_proxy_service())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/bangs.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn combined_app_has_connectivitycheck() {
        let ubo_service = test_service(
            "http://proxy.local/ubo/",
            "http://127.0.0.1:9/assets.json",
            "unused",
        );
        let response = app(ubo_service, test_extension_proxy_service())
            .oneshot(
                Request::builder()
                    .uri("/connectivitycheck")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn combined_app_redirects_root_to_helium_home() {
        let ubo_service = test_service(
            "http://proxy.local/ubo/",
            "http://127.0.0.1:9/assets.json",
            "unused",
        );
        let response = app(ubo_service, test_extension_proxy_service())
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.headers()[LOCATION], "https://helium.computer");
    }

    #[tokio::test]
    async fn combined_app_serves_robots_txt() {
        let ubo_service = test_service(
            "http://proxy.local/ubo/",
            "http://127.0.0.1:9/assets.json",
            "unused",
        );
        let response = app(ubo_service, test_extension_proxy_service())
            .oneshot(
                Request::builder()
                    .uri("/robots.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "text/plain");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"User-agent: *\nDisallow: /\n");
    }

    #[tokio::test]
    async fn combined_app_serves_robots_head_without_body() {
        let ubo_service = test_service(
            "http://proxy.local/ubo/",
            "http://127.0.0.1:9/assets.json",
            "unused",
        );
        let response = app(ubo_service, test_extension_proxy_service())
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/robots.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_LENGTH], "26");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn combined_app_rejects_unsupported_compat_methods() {
        let ubo_service = test_service(
            "http://proxy.local/ubo/",
            "http://127.0.0.1:9/assets.json",
            "unused",
        );
        let app = app(ubo_service, test_extension_proxy_service());

        let robots = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/robots.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(robots.status(), StatusCode::METHOD_NOT_ALLOWED);

        let updates = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/updates/mac")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(updates.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn mac_updates_proxy_preserves_path_query_and_response_headers() {
        let (source, requests) = recording_fixture_server("appcast").await;
        let app =
            crate::compat::app_with_mac_updates_base(Url::parse(&format!("{source}/mac")).unwrap())
                .unwrap();

        let base_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/updates/mac")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(base_response.status(), StatusCode::OK);

        let slash_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/updates/mac/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(slash_response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/updates/mac/stable/appcast.xml?channel=stable")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/xml");
        assert_eq!(response.headers()[ETAG], "\"fixture\"");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"appcast");

        let requests = requests.lock().unwrap();
        assert_eq!(
            requests.as_slice(),
            [
                "GET /mac",
                "GET /mac/",
                "GET /mac/stable/appcast.xml?channel=stable"
            ]
        );
    }

    fn test_service(base_url: &str, assets_url: &str, checksum: &str) -> UboService {
        UboService::new(Arc::new(Config {
            base_url: Url::parse(base_url).unwrap(),
            use_helium_assets: true,
            custom_assets_url: Some(Url::parse(assets_url).unwrap()),
            custom_assets_checksum: Some(checksum.to_string()),
        }))
    }

    fn test_extension_proxy_service() -> ExtensionProxyService {
        ExtensionProxyService::new(Arc::new(ExtensionProxyConfig {
            proxy_base_url: Some(Url::parse("http://proxy.local/ext/").unwrap()),
            hmac_secret: Some(b"abcdefghijklmnopqrstuvwxyz123456".to_vec()),
        }))
    }

    async fn fixture_server(body: &str) -> String {
        let body = body.to_string();
        let route = Router::new().fallback(move || {
            let body = body.clone();
            async move { body }
        });
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, route).await.unwrap();
        });
        format!("http://{addr}")
    }

    async fn recording_fixture_server(body: &str) -> (String, Arc<Mutex<Vec<String>>>) {
        let body = body.to_string();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let route_requests = Arc::clone(&requests);
        let route = Router::new().fallback(move |method: Method, uri: Uri| {
            let body = body.clone();
            let route_requests = Arc::clone(&route_requests);
            async move {
                route_requests
                    .lock()
                    .unwrap()
                    .push(format!("{method} {uri}"));

                let mut response = if method == Method::HEAD {
                    Response::new(Body::empty())
                } else {
                    Response::new(Body::from(body))
                };
                response
                    .headers_mut()
                    .insert(CONTENT_TYPE, "application/xml".parse().unwrap());
                response
                    .headers_mut()
                    .insert(ETAG, "\"fixture\"".parse().unwrap());
                response
            }
        });
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, route).await.unwrap();
        });
        (format!("http://{addr}"), requests)
    }

    fn brotli_decompress(body: &[u8]) -> String {
        let mut decompressed = Vec::new();
        let mut reader = brotli::Decompressor::new(body, 4096);
        std::io::copy(&mut reader, &mut decompressed).unwrap();
        String::from_utf8(decompressed).unwrap()
    }
}
