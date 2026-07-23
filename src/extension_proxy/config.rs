use std::env;
use std::fs;

use url::Url;

#[derive(Clone)]
pub struct ExtensionProxyConfig {
    pub(crate) proxy_base_url: Option<Url>,
    pub(crate) hmac_secret: Option<Vec<u8>>,
}

impl ExtensionProxyConfig {
    pub fn from_env() -> Result<Self, String> {
        let proxy_base_url = match env::var("PROXY_BASE_URL")
            .ok()
            .filter(|value| !value.is_empty())
        {
            Some(value) => Some(Url::parse(&value).map_err(|err| err.to_string())?),
            None => {
                eprintln!("PROXY_BASE_URL is not set, CRX requests will not be proxied");
                None
            }
        };

        let hmac_secret = resolve_hmac_secret(
            env::var("HMAC_SECRET_FILE").ok(),
            env::var("HMAC_SECRET").ok(),
        )?;

        Ok(Self {
            proxy_base_url,
            hmac_secret,
        })
    }

    pub(crate) fn proxying_enabled(&self) -> bool {
        self.proxy_base_url.is_some() && self.hmac_secret.is_some()
    }
}

/// Resolve the CRX-signing HMAC secret from its two possible sources.
///
/// Precedence: a `HMAC_SECRET_FILE` path (a `0600` file mounted as a Compose
/// secret) wins over the legacy inline `HMAC_SECRET` variable. The file is
/// authoritative — if it is configured but missing, unreadable, empty, or holds
/// fewer than 32 bytes, startup fails rather than silently falling back or
/// disabling proxying. The inline variable is a temporary migration fallback and
/// keeps the original lenient behaviour: absent or too-short simply disables
/// proxying with a warning.
///
/// The secret value is never included in any log line or error message; failures
/// report only the configured path.
fn resolve_hmac_secret(
    file_path: Option<String>,
    inline: Option<String>,
) -> Result<Option<Vec<u8>>, String> {
    if let Some(path) = file_path.filter(|value| !value.is_empty()) {
        let raw = fs::read(&path)
            .map_err(|err| format!("HMAC_SECRET_FILE ({path}) could not be read: {err}"))?;
        let secret = strip_trailing_newline(&raw);
        if secret.is_empty() {
            return Err(format!("HMAC_SECRET_FILE ({path}) is empty"));
        }
        if secret.len() < 32 {
            return Err(format!(
                "HMAC_SECRET_FILE ({path}) holds a secret shorter than 32 bytes"
            ));
        }
        return Ok(Some(secret.to_vec()));
    }

    match inline.filter(|value| !value.is_empty()) {
        Some(value) if value.len() >= 32 => Ok(Some(value.into_bytes())),
        _ => {
            eprintln!("HMAC_SECRET is not set or <32 chars, CRX requests will not be proxied");
            Ok(None)
        }
    }
}

/// Strip a single trailing newline (`\n`, or `\r\n`) so a secret file written
/// with one terminating newline yields the exact secret bytes.
fn strip_trailing_newline(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    // Exactly 32 bytes; the minimum the loader accepts.
    const SECRET_A: &[u8] = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const SECRET_B: &[u8] = b"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

    /// A temporary file that is removed on drop, so tests never touch global
    /// process state (unlike env vars) and can run in parallel.
    struct TempSecret {
        path: PathBuf,
    }

    impl TempSecret {
        fn new(contents: &[u8]) -> Self {
            let path = env::temp_dir().join(format!("helium-hmac-test-{}", uuid::Uuid::new_v4()));
            let mut file = fs::File::create(&path).unwrap();
            file.write_all(contents).unwrap();
            Self { path }
        }

        fn path_str(&self) -> String {
            self.path.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempSecret {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    #[test]
    fn file_takes_precedence_over_inline() {
        let file = TempSecret::new(SECRET_A);
        let inline = String::from_utf8(SECRET_B.to_vec()).unwrap();

        let secret = resolve_hmac_secret(Some(file.path_str()), Some(inline)).unwrap();

        assert_eq!(secret.as_deref(), Some(SECRET_A));
    }

    #[test]
    fn file_trailing_newline_is_stripped() {
        let mut with_lf = SECRET_A.to_vec();
        with_lf.push(b'\n');
        let file = TempSecret::new(&with_lf);

        let secret = resolve_hmac_secret(Some(file.path_str()), None).unwrap();

        // Exactly the secret, no newline and no truncation.
        assert_eq!(secret.as_deref(), Some(SECRET_A));
    }

    #[test]
    fn file_trailing_crlf_is_stripped() {
        let mut with_crlf = SECRET_A.to_vec();
        with_crlf.extend_from_slice(b"\r\n");
        let file = TempSecret::new(&with_crlf);

        let secret = resolve_hmac_secret(Some(file.path_str()), None).unwrap();

        assert_eq!(secret.as_deref(), Some(SECRET_A));
    }

    #[test]
    fn inline_used_when_file_absent() {
        let inline = String::from_utf8(SECRET_B.to_vec()).unwrap();

        let secret = resolve_hmac_secret(None, Some(inline)).unwrap();

        assert_eq!(secret.as_deref(), Some(SECRET_B));
    }

    #[test]
    fn empty_file_path_falls_back_to_inline() {
        let inline = String::from_utf8(SECRET_B.to_vec()).unwrap();

        let secret = resolve_hmac_secret(Some(String::new()), Some(inline)).unwrap();

        assert_eq!(secret.as_deref(), Some(SECRET_B));
    }

    #[test]
    fn missing_file_is_error_and_does_not_fall_back() {
        let inline = String::from_utf8(SECRET_B.to_vec()).unwrap();
        let missing = env::temp_dir()
            .join(format!("helium-hmac-missing-{}", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .into_owned();

        let err = resolve_hmac_secret(Some(missing), Some(inline)).unwrap_err();

        assert!(err.contains("could not be read"), "got: {err}");
        // Configured-but-broken file must fail, never silently use the inline value.
        assert!(!err.contains(&String::from_utf8(SECRET_B.to_vec()).unwrap()));
    }

    #[test]
    fn empty_file_is_error() {
        let file = TempSecret::new(b"");

        let err = resolve_hmac_secret(Some(file.path_str()), None).unwrap_err();

        assert!(err.contains("is empty"), "got: {err}");
    }

    #[test]
    fn newline_only_file_is_empty_error() {
        let file = TempSecret::new(b"\n");

        let err = resolve_hmac_secret(Some(file.path_str()), None).unwrap_err();

        assert!(err.contains("is empty"), "got: {err}");
    }

    #[test]
    fn short_file_is_error_and_value_is_redacted() {
        let short = b"too-short-secret\n"; // 16 bytes after stripping the newline
        let file = TempSecret::new(short);

        let err = resolve_hmac_secret(Some(file.path_str()), None).unwrap_err();

        assert!(err.contains("shorter than 32 bytes"), "got: {err}");
        // The error must never leak the secret contents.
        assert!(!err.contains("too-short-secret"), "secret leaked: {err}");
    }

    #[test]
    #[cfg(unix)]
    fn unreadable_file_is_error() {
        use std::os::unix::fs::PermissionsExt;

        let file = TempSecret::new(SECRET_A);
        fs::set_permissions(&file.path, fs::Permissions::from_mode(0o000)).unwrap();

        // A privileged test runner (e.g. root in CI) can read it regardless of
        // mode; only assert the failure when the mode actually denies us.
        if fs::read(&file.path).is_ok() {
            return;
        }

        let err = resolve_hmac_secret(Some(file.path_str()), None).unwrap_err();
        assert!(err.contains("could not be read"), "got: {err}");
    }

    #[test]
    fn no_source_disables_proxying() {
        let secret = resolve_hmac_secret(None, None).unwrap();
        assert!(secret.is_none());
    }

    #[test]
    fn short_inline_disables_proxying() {
        let secret = resolve_hmac_secret(None, Some("too-short".to_string())).unwrap();
        assert!(secret.is_none());
    }

    #[test]
    fn empty_inline_disables_proxying() {
        let secret = resolve_hmac_secret(None, Some(String::new())).unwrap();
        assert!(secret.is_none());
    }
}
