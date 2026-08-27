# Helium Service

A lightweight Rust backend that supports the [Helium browser](https://github.com/imputnet/helium-chromium): it proxies uBlock Origin filter lists/assets and Chrome Web Store extension traffic through a single, privacy-respecting endpoint.

## What it does

- **uBO asset proxy** (`/ubo`) — serves uBlock Origin's `assets.json` manifest and filter lists, rewriting upstream URLs to point back through this service, with HTTP caching (ETag/If-None-Match) and Brotli compression.
- **Extension proxy** (`/ext`, `/com`) — proxies Chrome Web Store extension update checks (Omaha protocol, v3/v4), Chrome component update checks, and CRX downloads, with HMAC-signed URLs so the origin server can't be reached directly by clients.
- **Helium bangs** (`/bangs.json`) — serves the generated community !bang definitions with long-lived caching and CORS.
- **Compatibility endpoints** (`/`, `/robots.txt`) — mirrors the community service root redirect and crawler policy.
- **Dictionary mirror** (`/dict`) — prepares Chromium Hunspell dictionaries during startup and serves the pre-gzipped mirror like the community nginx route.
- **Health checks** — `/healthz` is a cheap liveness check, `/readyz` gates traffic until uBO assets and the browser-compatible dictionary are active, and `/connectivitycheck` is the browser probe.

## Requirements

- Rust 1.96+ (edition 2024)

## Configuration

Copy `.env.example` to `.env` and adjust as needed:

| Variable | Required | Description |
|---|---|---|
| `HELIUM_BIND_ADDR` | No | Address to bind the HTTP server (default `0.0.0.0:8000`) |
| `HELIUM_HEALTHCHECK_URL` | No | URL used by `helium-service healthcheck` (default `http://127.0.0.1:8000/readyz`) |
| `UBO_PROXY_BASE_URL` | Yes | Public base URL this service is reachable at, used to rewrite asset URLs |
| `UBO_USE_ORIGINAL_UBLOCK_ASSETS` | No | Set `true` to serve upstream `gorhill/uBlock` assets instead of Helium's fork (default `false`) |
| `UBO_ASSETS_JSON_URL` | No | Override the `assets.json` source URL |
| `UBO_ASSETS_JSON_SHA256` | No | Expected SHA-256 checksum of the custom `assets.json` (required if `UBO_ASSETS_JSON_URL` is set) |
| `PROXY_BASE_URL` | No | Public base URL for the extension proxy; if unset, CRX proxying is disabled |
| `HMAC_SECRET_FILE` | No | File containing the secret (≥32 bytes) used to sign proxied URLs; takes precedence over `HMAC_SECRET`, and an invalid configured file fails startup |
| `HMAC_SECRET` | No | Legacy inline signing secret (≥32 chars); used only when `HMAC_SECRET_FILE` is unset |
| `DICT_MIRROR_DIR` | No | Writable local directory for the dictionary mirror (default `/tmp/helium-dictionaries`) |
| `DICT_TARBALL_URL` | No | Pinned Chromium Hunspell dictionary tarball URL |

## Running locally

```sh
cargo run
```

## Running with Docker

```sh
docker build -t helium-service .
docker run --env-file .env -p 8000:8000 helium-service
```

To reuse an unchanged dictionary mirror across container replacements, mount a
writable volume at `/tmp/helium-dictionaries` (or the configured
`DICT_MIRROR_DIR`). The service records the source URL with the mirror and only
downloads the archive when that marker or the required `en-US-10-1.bdic` is
missing.

## Operations

Request, upstream-failure, checksum-drift, dictionary-state, and hourly cache
statistics are emitted as JSON lines. Request logs use fixed route categories
and never include query strings, signed proxy URLs, or HMAC material.

## Testing

```sh
cargo test
```

Pull requests also run formatting and Clippy gates. A weekly read-only workflow
checks the pinned uBO metadata, generated bangs, and dictionary filename against
their upstream sources.

Hardening rationale and acceptance criteria are recorded in
[IMPROVEMENTS.md](IMPROVEMENTS.md).

## License

AGPL-3.0
