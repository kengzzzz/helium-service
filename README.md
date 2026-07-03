# Helium Service

A lightweight Rust backend that supports the [Helium browser](https://github.com/imputnet/helium-chromium): it proxies uBlock Origin filter lists/assets and Chrome Web Store extension traffic through a single, privacy-respecting endpoint.

## What it does

- **uBO asset proxy** (`/ubo`) — serves uBlock Origin's `assets.json` manifest and filter lists, rewriting upstream URLs to point back through this service, with HTTP caching (ETag/If-None-Match) and Brotli compression.
- **Extension proxy** (`/ext`) — proxies Chrome Web Store extension update checks (Omaha protocol, v3/v4) and CRX downloads, with HMAC-signed URLs so the origin server can't be reached directly by clients.
- **Health check** (`/healthz`) — returns `204 No Content` for container/orchestrator liveness probes.

## Requirements

- Rust 1.96+ (edition 2024)

## Configuration

Copy `.env.example` to `.env` and adjust as needed:

| Variable | Required | Description |
|---|---|---|
| `HELIUM_BIND_ADDR` | No | Address to bind the HTTP server (default `0.0.0.0:8000`) |
| `HELIUM_HEALTHCHECK_URL` | No | URL used by `helium-service healthcheck` (default `http://127.0.0.1:8000/healthz`) |
| `UBO_PROXY_BASE_URL` | Yes | Public base URL this service is reachable at, used to rewrite asset URLs |
| `UBO_USE_ORIGINAL_UBLOCK_ASSETS` | No | Set `true` to serve upstream `gorhill/uBlock` assets instead of Helium's fork (default `false`) |
| `UBO_ASSETS_JSON_URL` | No | Override the `assets.json` source URL |
| `UBO_ASSETS_JSON_SHA256` | No | Expected SHA-256 checksum of the custom `assets.json` (required if `UBO_ASSETS_JSON_URL` is set) |
| `PROXY_BASE_URL` | No | Public base URL for the extension proxy; if unset, CRX proxying is disabled |
| `HMAC_SECRET` | No | Secret (≥32 chars) used to sign proxied URLs; if unset, CRX proxying is disabled |

## Running locally

```sh
cargo run
```

## Running with Docker

```sh
docker build -t helium-service .
docker run --env-file .env -p 8000:8000 helium-service
```

## Testing

```sh
cargo test
```

## License

AGPL-3.0
