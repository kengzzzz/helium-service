use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use axum::{extract::Request, middleware::Next, response::Response};
use serde_json::json;

static UPSTREAM_FAILURES: AtomicU64 = AtomicU64::new(0);

pub(crate) async fn log_request(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let route = route_category(request.uri().path()).to_string();
    let started = Instant::now();
    let response = next.run(request).await;

    eprintln!(
        "{}",
        json!({
            "event": "request",
            "method": method.as_str(),
            "route": route,
            "status": response.status().as_u16(),
            "duration_ms": started.elapsed().as_millis(),
        })
    );
    response
}

pub(crate) fn upstream_failure(category: &'static str, kind: &'static str) {
    UPSTREAM_FAILURES.fetch_add(1, Ordering::Relaxed);
    eprintln!(
        "{}",
        json!({
            "event": "upstream_failure",
            "upstream": category,
            "kind": kind,
        })
    );
}

pub(crate) fn checksum_drift(category: &'static str) {
    eprintln!(
        "{}",
        json!({
            "event": "checksum_drift",
            "upstream": category,
        })
    );
}

pub(crate) fn upstream_failure_count() -> u64 {
    UPSTREAM_FAILURES.load(Ordering::Relaxed)
}

fn route_category(path: &str) -> &str {
    match path {
        "/" | "/healthz" | "/readyz" | "/connectivitycheck" | "/robots.txt" | "/bangs.json"
        | "/com" | "/dict" | "/dict/" | "/ubo/assets.json" | "/ext/proxy" | "/ext/cws_snippet"
        | "/ext/com" | "/ext/" => path,
        value if value.starts_with("/ubo/") => "/ubo/{filter}",
        value if value.starts_with("/dict/") => "/dict/{path}",
        value if value.starts_with("/ext/") => "/ext/{path}",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::route_category;

    #[test]
    fn route_categories_never_include_signed_queries_or_dynamic_paths() {
        assert_eq!(route_category("/ext/proxy"), "/ext/proxy");
        assert_eq!(route_category("/ubo/list/hash/file.txt"), "/ubo/{filter}");
        assert_eq!(route_category("/dict/en-US-10-1.bdic"), "/dict/{path}");
    }
}
