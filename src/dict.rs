use std::{
    env, fs,
    io::{self, Cursor, Read},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{
        HeaderValue, Method, StatusCode, Uri,
        header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, LOCATION},
    },
    response::Response,
    routing::get,
};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use tokio::{fs as tokio_fs, task, time};
use tokio_util::io::ReaderStream;
use url::Url;
use uuid::Uuid;

use crate::{
    error::ServiceError,
    upstream::{checked_response, read_limited},
};

pub(crate) const DEFAULT_TARBALL_URL: &str = "https://chromium.googlesource.com/chromium/deps/hunspell_dictionaries/+archive/cccf64a8acc951afe3f47fee023908e55699bc58.tar.gz";
const DEFAULT_MIRROR_DIR: &str = "/tmp/helium-dictionaries";
pub(crate) const REQUIRED_DICTIONARY: &str = "en-US-10-1.bdic";
const SOURCE_MARKER: &str = ".source-url";
pub(crate) const TARBALL_LIMIT: usize = 80 * 1024 * 1024;
const STARTUP_RETRIES: usize = 3;

#[derive(Clone)]
pub(crate) struct DictionaryService {
    client: reqwest::Client,
    mirror_dir: Arc<PathBuf>,
    tarball_url: Url,
    ready: Arc<AtomicBool>,
}

impl DictionaryService {
    pub(crate) fn from_env() -> Result<Self, String> {
        let mirror_dir = env::var("DICT_MIRROR_DIR")
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_MIRROR_DIR));
        let tarball_url = env::var("DICT_TARBALL_URL")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_TARBALL_URL.to_string());
        Ok(Self::new(
            mirror_dir,
            Url::parse(&tarball_url).map_err(|err| err.to_string())?,
        ))
    }

    fn new(mirror_dir: PathBuf, tarball_url: Url) -> Self {
        let ready = mirror_is_current(&mirror_dir, tarball_url.as_str());
        Self {
            client: crate::upstream::metadata_client()
                .expect("metadata HTTP client configuration must be valid"),
            mirror_dir: Arc::new(mirror_dir),
            tarball_url,
            ready: Arc::new(AtomicBool::new(ready)),
        }
    }

    pub(crate) fn default() -> Self {
        Self::new(
            PathBuf::from(DEFAULT_MIRROR_DIR),
            Url::parse(DEFAULT_TARBALL_URL).expect("default dictionary tarball URL must be valid"),
        )
    }

    pub(crate) fn spawn_startup_refresh(&self) {
        let service = self.clone();
        tokio::spawn(async move {
            service.refresh_at_startup().await;
        });
    }

    async fn refresh_at_startup(&self) {
        if self.is_ready() {
            log_dictionary_state("reused", 0);
            return;
        }

        for attempt in 1..=STARTUP_RETRIES {
            match self.refresh_once().await {
                Ok(()) => {
                    log_dictionary_state("active", attempt);
                    return;
                }
                Err(_) => log_dictionary_state("retrying", attempt),
            }
            if attempt < STARTUP_RETRIES {
                time::sleep(Duration::from_secs(2)).await;
            }
        }
        log_dictionary_state("unready", STARTUP_RETRIES);
    }

    async fn refresh_once(&self) -> Result<(), ServiceError> {
        let response = checked_response(
            self.client.get(self.tarball_url.clone()).send().await,
            "dictionary",
        )
        .await?;
        let tarball = read_limited(response, TARBALL_LIMIT, "dictionary").await?;
        let mirror_dir = Arc::clone(&self.mirror_dir);
        let source_url = self.tarball_url.to_string();

        task::spawn_blocking(move || {
            refresh_from_tarball(&mirror_dir, tarball.as_ref(), &source_url)
        })
        .await
        .map_err(ServiceError::internal)?
        .map_err(ServiceError::internal)?;
        self.ready.store(true, Ordering::Release);
        Ok(())
    }

    fn active_dir(&self) -> PathBuf {
        self.mirror_dir.join("dict")
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn for_test(ready: bool) -> Self {
        let service = Self::new(
            env::temp_dir().join(format!("helium-service-ready-test-{}", Uuid::new_v4())),
            Url::parse(DEFAULT_TARBALL_URL).unwrap(),
        );
        service.ready.store(ready, Ordering::Release);
        service
    }

    #[cfg(test)]
    pub(crate) fn set_ready_for_test(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }
}

fn log_dictionary_state(state: &'static str, attempt: usize) {
    eprintln!(
        "{}",
        serde_json::json!({
            "event": "dictionary_refresh",
            "state": state,
            "attempt": attempt,
        })
    );
}

pub(crate) fn app(service: DictionaryService) -> Router {
    Router::new()
        .route("/dict", get(redirect_to_root).head(redirect_to_root))
        .route("/dict/", get(handle).head(handle))
        .route("/dict/{*path}", get(handle).head(handle))
        .with_state(service)
}

async fn redirect_to_root() -> Result<Response, ServiceError> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
    response
        .headers_mut()
        .insert(LOCATION, HeaderValue::from_static("/dict/"));
    Ok(response)
}

async fn handle(
    State(service): State<DictionaryService>,
    method: Method,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let relative = safe_relative_path(uri.path())?;
    let active_dir = service.active_dir();
    let include_body = method == Method::GET;

    let response = dictionary_response(&active_dir, &relative, include_body).await?;
    Ok(response)
}

fn safe_relative_path(path: &str) -> Result<PathBuf, ServiceError> {
    let relative = path
        .strip_prefix("/dict")
        .unwrap_or(path)
        .trim_start_matches('/');
    let mut output = PathBuf::new();

    for component in Path::new(relative).components() {
        match component {
            Component::Normal(value) => output.push(value),
            Component::CurDir => {}
            _ => {
                return Err(ServiceError::with_status(
                    StatusCode::BAD_REQUEST,
                    "invalid dictionary path",
                ));
            }
        }
    }

    Ok(output)
}

async fn dictionary_response(
    active_dir: &Path,
    relative: &Path,
    include_body: bool,
) -> Result<Response, ServiceError> {
    let directory = active_dir.join(relative);
    if tokio_fs::metadata(&directory)
        .await
        .is_ok_and(|metadata| metadata.is_dir())
    {
        let active_dir = active_dir.to_path_buf();
        let relative = relative.to_path_buf();
        return task::spawn_blocking(move || {
            directory_listing(&active_dir, &relative, include_body)
        })
        .await
        .map_err(ServiceError::internal)?;
    }

    let file = active_dir.join(gzip_path(relative));
    let metadata = match tokio_fs::metadata(&file).await {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => {
            return Err(ServiceError::with_status(
                StatusCode::NOT_FOUND,
                "Not Found",
            ));
        }
    };

    let body = if include_body {
        let file = tokio_fs::File::open(file)
            .await
            .map_err(ServiceError::internal)?;
        Body::from_stream(ReaderStream::new(file))
    } else {
        Body::empty()
    };
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&metadata.len().to_string()).map_err(ServiceError::internal)?,
    );
    Ok(response)
}

fn gzip_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.gz", path.display()))
}

fn directory_listing(
    active_dir: &Path,
    relative: &Path,
    include_body: bool,
) -> Result<Response, ServiceError> {
    let directory = active_dir.join(relative);
    if !directory.is_dir() {
        return Err(ServiceError::with_status(
            StatusCode::NOT_FOUND,
            "Not Found",
        ));
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(directory).map_err(ServiceError::internal)? {
        let entry = entry.map_err(ServiceError::internal)?;
        let metadata = entry.metadata().map_err(ServiceError::internal)?;
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if file_name.starts_with('.') {
            continue;
        }
        let display = if metadata.is_file() {
            file_name
                .strip_suffix(".gz")
                .unwrap_or(&file_name)
                .to_string()
        } else {
            format!("{file_name}/")
        };
        entries.push(display);
    }
    entries.sort();

    let prefix = if relative.as_os_str().is_empty() {
        "/dict/".to_string()
    } else {
        format!("/dict/{}/", relative.display())
    };
    let mut html = String::from("<html><body><pre>\n");
    for entry in entries {
        html.push_str(&format!(
            "<a href=\"{}{}\">{}</a>\n",
            prefix,
            html_escape(&entry),
            html_escape(&entry)
        ));
    }
    html.push_str("</pre></body></html>\n");

    let body = if include_body {
        Body::from(html.clone())
    } else {
        Body::empty()
    };
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&html.len().to_string()).map_err(ServiceError::internal)?,
    );
    Ok(response)
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn refresh_from_tarball(mirror_dir: &Path, tarball: &[u8], source_url: &str) -> Result<(), String> {
    fs::create_dir_all(mirror_dir).map_err(|err| err.to_string())?;

    let id = Uuid::new_v4();
    let active = mirror_dir.join("dict");
    let tmp = mirror_dir.join(format!("tmp-{id}"));
    let old = mirror_dir.join(format!("old-{id}"));

    let result = (|| {
        fs::create_dir(&tmp).map_err(|err| err.to_string())?;
        unpack_tarball(tarball, &tmp)?;
        if !gzip_path(&tmp.join(REQUIRED_DICTIONARY)).is_file() {
            return Err(format!(
                "dictionary archive is missing {REQUIRED_DICTIONARY}"
            ));
        }
        fs::write(tmp.join(SOURCE_MARKER), source_url).map_err(|err| err.to_string())?;

        if active.exists() {
            fs::rename(&active, &old).map_err(|err| err.to_string())?;
        }

        if let Err(err) = fs::rename(&tmp, &active) {
            if old.exists() && !active.exists() {
                let _ = fs::rename(&old, &active);
            }
            return Err(err.to_string());
        }

        if old.exists() {
            fs::remove_dir_all(&old).map_err(|err| err.to_string())?;
        }

        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&tmp);
    }

    result
}

fn mirror_is_current(mirror_dir: &Path, source_url: &str) -> bool {
    let active = mirror_dir.join("dict");
    gzip_path(&active.join(REQUIRED_DICTIONARY)).is_file()
        && fs::read_to_string(active.join(SOURCE_MARKER)).is_ok_and(|marker| marker == source_url)
}

fn unpack_tarball(tarball: &[u8], target: &Path) -> Result<(), String> {
    let decoder = GzDecoder::new(Cursor::new(tarball));
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries().map_err(|err| err.to_string())? {
        let mut entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path().map_err(|err| err.to_string())?;
        let safe_path = safe_archive_path(&path)?;
        if safe_path.as_os_str().is_empty() {
            continue;
        }

        let output = target.join(safe_path);
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(output).map_err(|err| err.to_string())?;
            continue;
        }
        if !entry.header().entry_type().is_file() {
            continue;
        }

        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        gzip_archive_entry(&mut entry, &gzip_path(&output))?;
    }

    Ok(())
}

fn gzip_archive_entry<R: Read>(entry: &mut tar::Entry<'_, R>, path: &Path) -> Result<(), String> {
    let output = fs::File::create(path)
        .map_err(|err| format!("failed to create `{}`: {err}", path.display()))?;
    let mut encoder = GzEncoder::new(output, Compression::best());
    io::copy(entry, &mut encoder).map_err(|err| {
        format!(
            "failed to gzip archive entry into `{}`: {err}",
            path.display()
        )
    })?;
    encoder
        .finish()
        .map_err(|err| format!("failed to finish `{}`: {err}", path.display()))?;
    Ok(())
}

fn safe_archive_path(path: &Path) -> Result<PathBuf, String> {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => output.push(value),
            Component::CurDir => {}
            _ => return Err("unsafe dictionary archive path".to_string()),
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::{
        net::SocketAddr,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use axum::{
        body::Body,
        http::{
            Request, StatusCode,
            header::{CONTENT_ENCODING, CONTENT_LENGTH},
        },
    };
    use flate2::read::GzDecoder;
    use http_body_util::BodyExt;
    use tar::{Builder, Header};
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    use super::*;

    #[test]
    fn default_tarball_is_pinned_to_browser_compatible_revision() {
        assert_eq!(
            DEFAULT_TARBALL_URL,
            "https://chromium.googlesource.com/chromium/deps/hunspell_dictionaries/+archive/cccf64a8acc951afe3f47fee023908e55699bc58.tar.gz"
        );
        assert!(!DEFAULT_TARBALL_URL.contains("refs/heads"));
    }

    #[tokio::test]
    async fn refresh_serves_extensionless_gzip_file_and_head() {
        let service = fixture_service(dictionary_tarball(&[("en-US.dic", "word\n")])).await;
        service.refresh_once().await.unwrap();
        let app = app(service);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dict/en-US.dic")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_ENCODING], "gzip");
        let content_length = response.headers()[CONTENT_LENGTH]
            .to_str()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.len(), content_length);
        assert_eq!(gzip_decompress(&body), "word\n");

        let head = app
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/dict/en-US.dic")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(head.status(), StatusCode::OK);
        assert_eq!(head.headers()[CONTENT_LENGTH], content_length.to_string());
        let body = head.into_body().collect().await.unwrap().to_bytes();
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn listing_hides_gz_suffix() {
        let service = fixture_service(dictionary_tarball(&[
            ("en-US.dic", "word\n"),
            ("subdir/custom.aff", "SET UTF-8\n"),
        ]))
        .await;
        service.refresh_once().await.unwrap();

        let response = app(service)
            .oneshot(
                Request::builder()
                    .uri("/dict/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("en-US.dic"));
        assert!(body.contains("subdir/"));
        assert!(!body.contains(".gz"));
    }

    #[tokio::test]
    async fn traversal_paths_are_rejected() {
        let service = fixture_service(dictionary_tarball(&[])).await;

        let response = app(service)
            .oneshot(
                Request::builder()
                    .uri("/dict/../../Cargo.toml")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn refresh_failure_preserves_previous_mirror() {
        let mirror_dir = temp_dir();
        let first_url = fixture_server(
            StatusCode::OK,
            dictionary_tarball(&[("en-US.dic", "old\n")]),
        )
        .await;
        let first = DictionaryService::new(mirror_dir.clone(), Url::parse(&first_url).unwrap());
        first.refresh_once().await.unwrap();

        let failing_url = fixture_server(StatusCode::INTERNAL_SERVER_ERROR, Vec::new()).await;
        let second = DictionaryService::new(mirror_dir, Url::parse(&failing_url).unwrap());
        assert!(second.refresh_once().await.is_err());

        let response = app(second)
            .oneshot(
                Request::builder()
                    .uri("/dict/en-US.dic")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(gzip_decompress(&body), "old\n");
    }

    #[tokio::test]
    async fn warm_start_reuses_matching_mirror_without_downloading() {
        let mirror_dir = temp_dir();
        let requests = Arc::new(AtomicUsize::new(0));
        let (url, _) = counted_fixture_server(
            StatusCode::OK,
            dictionary_tarball(&[]),
            Arc::clone(&requests),
        )
        .await;
        let first = DictionaryService::new(mirror_dir.clone(), Url::parse(&url).unwrap());
        first.refresh_once().await.unwrap();
        assert_eq!(requests.load(Ordering::Relaxed), 1);

        let warm = DictionaryService::new(mirror_dir, Url::parse(&url).unwrap());
        assert!(warm.is_ready());
        warm.refresh_at_startup().await;
        assert_eq!(requests.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn readiness_requires_browser_compatible_dictionary() {
        let mirror_dir = temp_dir();
        let url = fixture_server(StatusCode::OK, dictionary_tarball(&[])).await;
        let service = DictionaryService::new(mirror_dir.clone(), Url::parse(&url).unwrap());
        service.refresh_once().await.unwrap();
        assert!(service.is_ready());

        fs::remove_file(gzip_path(
            &mirror_dir.join("dict").join(REQUIRED_DICTIONARY),
        ))
        .unwrap();
        let restarted = DictionaryService::new(mirror_dir, Url::parse(&url).unwrap());
        assert!(!restarted.is_ready());
    }

    async fn fixture_service(tarball: Vec<u8>) -> DictionaryService {
        let url = fixture_server(StatusCode::OK, tarball).await;
        DictionaryService::new(temp_dir(), Url::parse(&url).unwrap())
    }

    async fn fixture_server(status: StatusCode, body: Vec<u8>) -> String {
        counted_fixture_server(status, body, Arc::new(AtomicUsize::new(0)))
            .await
            .0
    }

    async fn counted_fixture_server(
        status: StatusCode,
        body: Vec<u8>,
        requests: Arc<AtomicUsize>,
    ) -> (String, Arc<AtomicUsize>) {
        let route_requests = Arc::clone(&requests);
        let route = Router::new().fallback(move || {
            let body = body.clone();
            let requests = Arc::clone(&route_requests);
            async move {
                requests.fetch_add(1, Ordering::Relaxed);
                let mut response = Response::new(Body::from(body));
                *response.status_mut() = status;
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

    fn dictionary_tarball(files: &[(&str, &str)]) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = Builder::new(encoder);
        let compatible = (REQUIRED_DICTIONARY, "compatible\n");
        for (path, contents) in files.iter().copied().chain(
            (!files.iter().any(|(path, _)| *path == REQUIRED_DICTIONARY)).then_some(compatible),
        ) {
            let mut header = Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, contents.as_bytes()).unwrap();
        }
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap()
    }

    fn gzip_decompress(body: &[u8]) -> String {
        let mut decompressed = String::new();
        let mut decoder = GzDecoder::new(body);
        decoder.read_to_string(&mut decompressed).unwrap();
        decompressed
    }

    fn temp_dir() -> PathBuf {
        env::temp_dir().join(format!("helium-service-dict-test-{}", Uuid::new_v4()))
    }
}
